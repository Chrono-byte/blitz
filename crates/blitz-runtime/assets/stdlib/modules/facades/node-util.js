import types from "node:util/types";

const inspectCustom = Symbol.for("nodejs.util.inspect.custom");
const promisifyCustom = Symbol.for("nodejs.util.promisify.custom");
const callbackifyCustom = Symbol.for("nodejs.util.callbackify.custom");
const hasOwn = (value, key) => Object.prototype.hasOwnProperty.call(value, key);

const defaultInspectOptions = {
  depth: 2,
  colors: false,
  customInspect: true,
  maxArrayLength: 100,
  maxStringLength: 10000,
  compact: 3,
  breakLength: 80,
};
const inspectOptions = options => {
  if (typeof options === "number") return { ...defaultInspectOptions, depth: options };
  return options === null || options === undefined ? { ...defaultInspectOptions } : { ...defaultInspectOptions, ...options };
};

const quote = value => {
  const text = String(value);
  return "'" + text
    .replace(/\\/g, "\\\\")
    .replace(/'/g, "\\'")
    .replace(/\n/g, "\\n")
    .replace(/\r/g, "\\r")
    .replace(/\t/g, "\\t")
    .replace(/\u0008/g, "\\b")
    .replace(/\u000c/g, "\\f") + "'";
};

const colorCodes = {
  special: [36, 39],
  number: [33, 39],
  bigint: [33, 39],
  boolean: [33, 39],
  undefined: [90, 39],
  null: [1, 22],
  string: [32, 39],
  symbol: [32, 39],
  date: [35, 39],
  regexp: [31, 39],
};
const stylize = (text, style, options) => {
  if (options.colors !== true) return text;
  const code = colorCodes[style];
  return code === undefined ? text : "\u001b[" + code[0] + "m" + text + "\u001b[" + code[1] + "m";
};

const objectTag = value => {
  try {
    return Object.prototype.toString.call(value);
  } catch {
    return "[object Object]";
  }
};

const isObjectLike = value => value !== null && (typeof value === "object" || typeof value === "function");
const seenValue = (seen, value) => seen.indexOf(value) !== -1;
const maxItems = options => options.maxArrayLength === null || options.maxArrayLength === undefined || options.maxArrayLength === Infinity
  ? Infinity
  : Math.max(0, Number(options.maxArrayLength));

const inspectValue = (value, options, depth, seen) => {
  if (value === undefined) return stylize("undefined", "undefined", options);
  if (value === null) return stylize("null", "null", options);
  if (typeof value === "string") {
    const max = options.maxStringLength === null || options.maxStringLength === undefined || options.maxStringLength === Infinity
      ? Infinity
      : Math.max(0, Number(options.maxStringLength));
    const text = max === Infinity || value.length <= max ? value : value.slice(0, max) + "... " + (value.length - max) + " more characters";
    return stylize(quote(text), "string", options);
  }
  if (typeof value === "number") {
    if (value !== value) return stylize("NaN", "number", options);
    if (value === Infinity) return stylize("Infinity", "number", options);
    if (value === -Infinity) return stylize("-Infinity", "number", options);
    if (value === 0 && 1 / value === -Infinity) return stylize("-0", "number", options);
    return stylize(String(value), "number", options);
  }
  if (typeof value === "boolean") return stylize(String(value), "boolean", options);
  if (typeof value === "bigint") return stylize(String(value) + "n", "bigint", options);
  if (typeof value === "symbol") return stylize(String(value), "symbol", options);
  if (typeof value === "function") {
    let name = "";
    try { name = value.name; } catch {}
    return "[Function" + (name === "" ? " (anonymous)" : ": " + name) + "]";
  }

  if (options.customInspect !== false) {
    try {
      const custom = value[inspectCustom];
      if (typeof custom === "function") {
        const result = custom.call(value, options.depth === null ? Infinity : depth, options, inspect);
        if (typeof result === "string") return result;
        if (result !== value) return inspectValue(result, options, depth, seen);
      }
    } catch {}
  }

  if (options.depth !== null && options.depth !== undefined && depth > Number(options.depth)) {
    const tag = objectTag(value);
    return tag === "[object Array]" ? "[Array]" : "[Object]";
  }
  if (seenValue(seen, value)) return "[Circular]";
  seen.push(value);

  let result;
  const tag = objectTag(value);
  if (tag === "[object Date]") {
    try { result = value.toISOString(); } catch { result = "Invalid Date"; }
    result = stylize(result, "date", options);
  } else if (tag === "[object RegExp]") {
    result = stylize(String(value), "regexp", options);
  } else if (Array.isArray(value)) {
    const values = [];
    const limit = Math.min(value.length, maxItems(options));
    for (let index = 0; index < limit; index += 1) values.push(hasOwn(value, index) ? inspectValue(value[index], options, depth + 1, seen) : "<empty>");
    if (limit < value.length) values.push("... " + (value.length - limit) + " more item" + (value.length - limit === 1 ? "" : "s"));
    result = "[ " + values.join(", ") + " ]";
  } else if (tag === "[object Map]") {
    const entries = [];
    try {
      value.forEach((entryValue, entryKey) => {
        if (entries.length < maxItems(options)) entries.push(inspectValue(entryKey, options, depth + 1, seen) + " => " + inspectValue(entryValue, options, depth + 1, seen));
      });
    } catch {}
    result = "Map { " + entries.join(", ") + " }";
  } else if (tag === "[object Set]") {
    const entries = [];
    try {
      value.forEach(entry => {
        if (entries.length < maxItems(options)) entries.push(inspectValue(entry, options, depth + 1, seen));
      });
    } catch {}
    result = "Set { " + entries.join(", ") + " }";
  } else {
    const keys = Object.keys(value);
    if (options.sorted === true) keys.sort();
    const properties = [];
    const limit = Math.min(keys.length, maxItems(options));
    for (let index = 0; index < limit; index += 1) {
      const key = keys[index];
      let propertyValue;
      try { propertyValue = value[key]; } catch { propertyValue = "[Getter]"; }
      properties.push(/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key) ? key + ": " + inspectValue(propertyValue, options, depth + 1, seen) : quote(key) + ": " + inspectValue(propertyValue, options, depth + 1, seen));
    }
    if (limit < keys.length) properties.push("... " + (keys.length - limit) + " more item" + (keys.length - limit === 1 ? "" : "s"));
    result = "{ " + properties.join(", ") + " }";
  }
  seen.pop();
  return result;
};

function inspect(value, options) {
  return inspectValue(value, inspectOptions(options), 0, []);
}
inspect.custom = inspectCustom;
inspect.defaultOptions = defaultInspectOptions;

const formatValue = (value, options, specifier) => {
  if (specifier === "s") {
    if (typeof value === "string") return value;
    return isObjectLike(value) ? inspect(value, options) : String(value);
  }
  if (specifier === "d") return String(Number(value));
  if (specifier === "i") return String(parseInt(value, 10));
  if (specifier === "f") return String(parseFloat(value));
  if (specifier === "j") {
    try {
      const json = JSON.stringify(value);
      return json === undefined ? "undefined" : json;
    } catch {
      return "[Circular]";
    }
  }
  if (specifier === "o") {
    const objectOptions = { ...options, showHidden: true, showProxy: true, depth: 4 };
    return inspect(value, objectOptions);
  }
  return inspect(value, options);
};

function formatWithOptions(options, value, ...args) {
  const inspectOpts = inspectOptions(options);
  if (typeof value !== "string") return [value, ...args].map(item => isObjectLike(item) ? inspect(item, inspectOpts) : String(item)).join(" ");
  let index = 0;
  const output = value.replace(/%[sdifjoOc%]/g, token => {
    if (token === "%%") return "%";
    if (token === "%c") {
      index += 1;
      return "";
    }
    if (index >= args.length) return token;
    return formatValue(args[index++], inspectOpts, token.slice(1));
  });
  if (index >= args.length) return output;
  return output + " " + args.slice(index).map(item => isObjectLike(item) ? inspect(item, inspectOpts) : String(item)).join(" ");
}
function format(value, ...args) {
  return formatWithOptions({}, value, ...args);
}

function inherits(constructor, superConstructor) {
  if (typeof constructor !== "function" || typeof superConstructor !== "function") throw new TypeError("The constructor and superConstructor arguments must be functions");
  constructor.super_ = superConstructor;
  constructor.prototype = Object.create(superConstructor.prototype, {
    constructor: { value: constructor, writable: true, configurable: true },
  });
  return constructor;
}

function callbackify(original) {
  if (typeof original !== "function") throw new TypeError("The callback argument must be of type function");
  const custom = original[callbackifyCustom];
  if (typeof custom === "function") return custom;
  return function(...args) {
    const callback = args.pop();
    if (typeof callback !== "function") throw new TypeError("The last argument must be of type function");
    let promise;
    try {
      promise = original.apply(this, args);
    } catch (error) {
      callback(error);
      return;
    }
    Promise.resolve(promise).then(
      value => callback(null, value),
      error => callback(error),
    );
  };
}
callbackify.custom = callbackifyCustom;

function promisify(original) {
  if (typeof original !== "function") throw new TypeError("The original argument must be of type function");
  const custom = original[promisifyCustom];
  if (typeof custom === "function") return custom;
  return function(...args) {
    return new Promise((resolve, reject) => {
      const callback = (error, ...values) => {
        if (error !== null && error !== undefined) reject(error);
        else resolve(values.length > 1 ? values : values[0]);
      };
      try {
        original.call(this, ...args, callback);
      } catch (error) {
        reject(error);
      }
    });
  };
}
promisify.custom = promisifyCustom;

const warned = [];
function deprecate(fn, message, code) {
  if (typeof fn !== "function") throw new TypeError("The first argument must be a function");
  let warnedOnce = false;
  return function(...args) {
    if (!warnedOnce && warned.indexOf(code || message) === -1) {
      warnedOnce = true;
      warned.push(code || message);
      const warning = "[DEP" + (code === undefined ? "" : " " + code) + "] " + message;
      if (globalThis.console && typeof globalThis.console.warn === "function") globalThis.console.warn(warning);
    }
    return fn.apply(this, args);
  };
}

function debuglog(section, callback) {
  const name = String(section).toUpperCase();
  const processValue = globalThis.process;
  const enabled = processValue !== undefined && processValue.env !== undefined
    && typeof processValue.env.NODE_DEBUG === "string"
    && processValue.env.NODE_DEBUG.toUpperCase().split(/[ ,]+/).indexOf(name) !== -1;
  const logger = enabled
    ? (message, ...args) => {
      const text = format(message, ...args);
      if (globalThis.console && typeof globalThis.console.error === "function") globalThis.console.error(name + " " + text);
    }
    : () => undefined;
  logger.enabled = enabled;
  if (typeof callback === "function") callback(logger);
  return logger;
}
debuglog.enabled = section => debuglog(section).enabled;
const debug = debuglog;

function stripVTControlCharacters(value) {
  return String(value)
    .replace(/\u001B\][^\u0007]*(?:\u0007|\u001B\\)/g, "")
    .replace(/\u001B\[[0-?]*[ -/]*[@-~]|\u009B[0-?]*[ -/]*[@-~]/g, "");
}

const util = {
  format,
  formatWithOptions,
  inspect,
  inherits,
  callbackify,
  promisify,
  deprecate,
  debuglog,
  debug,
  stripVTControlCharacters,
  types,
};

export {
  callbackify,
  debug,
  debuglog,
  deprecate,
  format,
  formatWithOptions,
  inherits,
  inspect,
  promisify,
  stripVTControlCharacters,
  types,
};
export { util as default, util };
