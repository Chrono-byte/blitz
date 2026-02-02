/**
 * Prelude: loaded first by the VM to set up globals (std + more).
 * All of this runs on the Rust VM; the actual runtime is in Rust.
 * This file prepares the environment for user code that expects a full ECMAScript
 * environment, including all built-in objects and functions.
 */

import type {
  Value,
  Console,
  ObjectConstructor,
  ArrayConstructor,
  StringConstructor,
  BlitzObject,
  BlitzFunction,
} from "./core/types.js";
import { BlitzObject as ObjectImpl, BlitzFunction as FunctionImpl } from "./core/objects.js";
import { toString } from "./core/runtime.js";
// Host helpers are provided by the inlined `soft/host.ts` module and expose
// top-level functions such as `host_print`, `native_new_object`, `native_print`
// and `debugValue`. Import them explicitly from the host module so the soft
// prelude doesn't accidentally shadow or redefine these helpers.
import {
  host_print,
  native_new_object,
  native_print,
  debugValue as host_debugValue,
  host as host_ns,
} from "./host.js";

// Note: std modules are imported after we establish canonical runtime
// constructors (Object, Array, String, console). The std code mutates
// `.prototype` on those constructors, so we must ensure the host/runtime
// provides a usable prototype object before evaluating std modules.

// Note: we imported the runtime helpers above. For clarity expose local
// aliases used by the rest of the prelude.
const host = host_ns;
const debugValue = host_debugValue;

// Lazy resolver that caches the host/native print function to avoid
// performing host-bridge lookups on every console call. It first tries
// to use globally published helpers (getHostGetter/getBuiltinCached) which
// `soft/host.ts` exposes during bootstrap. If those are not available it
// falls back to the imported `native_print` wrapper and finally to
// `host_print` as a last resort.
const getNativePrinter = (() => {
  let cached: any = undefined;
  let initialized = false;
  return function (): any {
    if (initialized) return cached;
    initialized = true;
    try {
      if (typeof globalThis !== "undefined") {
        const g: any = globalThis as any;
        if (g && typeof g.getHostGetter === "function" && typeof g.getBuiltinCached === "function") {
          try {
            const getter = g.getHostGetter();
            if (typeof getter === "function") {
              const p = g.getBuiltinCached(getter, "__native_print");
              if (typeof p === "function") {
                cached = p;
                return cached;
              }
            }
          } catch (_) {}
        }
      }
    } catch (_) {}
    // Fallbacks
    try {
      if (typeof native_print === "function") cached = native_print;
    } catch (_) {}
    if (!cached) cached = host_print;
    return cached;
  };
})();

// Declare global variables
// Capture runtime globals (console, Object, Array, String) when available so
// the bundler-generated TypeScript files can reference them without
// relying on unsupported `declare` statements.
let console: any =
  typeof globalThis !== "undefined" && (globalThis as any).console
    ? (globalThis as any).console
    : { log: function (..._args: any[]) {} };
let Object: any =
  typeof globalThis !== "undefined" && (globalThis as any).Object ? (globalThis as any).Object : {};
let Array: any =
  typeof globalThis !== "undefined" && (globalThis as any).Array ? (globalThis as any).Array : {};
let String: any =
  typeof globalThis !== "undefined" && (globalThis as any).String ? (globalThis as any).String : {};

// Bootstrap global console with basic implementation
const globalConsole: Console = {
  log: function (...args: Value[]): void {
    let out = "";
    let sep = "";
    let i = 0;
    while (i < (args as any).length) {
      const a = (args as any)[i];
      let s: string;
      if (a === null) s = "null";
      else if (typeof a === "undefined") s = "undefined";
      else if (typeof a === "string") s = a;
      else if (typeof a === "number") s = String(a);
      else if (typeof a === "boolean") s = a ? "true" : "false";
      else s = "[object Object]";
      out = out + sep + s;
      sep = " ";
      i = i + 1;
    }
    native_print(out);
  },
  warn: function (...args: Value[]): void {
    let out = "";
    let sep = "";
    let i = 0;
    while (i < (args as any).length) {
      const a = (args as any)[i];
      let s: string;
      if (a === null) s = "null";
      else if (typeof a === "undefined") s = "undefined";
      else if (typeof a === "string") s = a;
      else if (typeof a === "number") s = String(a);
      else if (typeof a === "boolean") s = a ? "true" : "false";
      else s = "[object Object]";
      out = out + sep + s;
      sep = " ";
      i = i + 1;
    }
    native_print(out);
  },
  error: function (...args: Value[]): void {
    let out = "";
    let sep = "";
    let i = 0;
    while (i < (args as any).length) {
      const a = (args as any)[i];
      let s: string;
      if (a === null) s = "null";
      else if (typeof a === "undefined") s = "undefined";
      else if (typeof a === "string") s = a;
      else if (typeof a === "number") s = String(a);
      else if (typeof a === "boolean") s = a ? "true" : "false";
      else s = "[object Object]";
      out = out + sep + s;
      sep = " ";
      i = i + 1;
    }
    native_print(out);
  },
  info: function (...args: Value[]): void {
    let out = "";
    let sep = "";
    let i = 0;
    while (i < (args as any).length) {
      const a = (args as any)[i];
      let s: string;
      if (a === null) s = "null";
      else if (typeof a === "undefined") s = "undefined";
      else if (typeof a === "string") s = a;
      else if (typeof a === "number") s = String(a);
      else if (typeof a === "boolean") s = a ? "true" : "false";
      else s = "[object Object]";
      out = out + sep + s;
      sep = " ";
      i = i + 1;
    }
    native_print(out);
  },
  debug: function (...args: Value[]): void {
    let out = "";
    let sep = "";
    let i = 0;
    while (i < (args as any).length) {
      const a = (args as any)[i];
      let s: string;
      if (a === null) s = "null";
      else if (typeof a === "undefined") s = "undefined";
      else if (typeof a === "string") s = a;
      else if (typeof a === "number") s = String(a);
      else if (typeof a === "boolean") s = a ? "true" : "false";
      else s = "[object Object]";
      out = out + sep + s;
      sep = " ";
      i = i + 1;
    }
    native_print(out);
  },
};

// Build and assign `console` with a single stable assignment to avoid creating
// multiple intermediate hidden classes during bootstrap. Construct `console`
// privately, set its properties in a stable order, then publish it to
// `globalThis` in one write. If a host-provided console already exists, prefer
// it but still copy methods in a stable way.
try {
  const builtConsole: any = {};
  // Set properties in a fixed order to stabilize hidden classes.
  // For `log` we provide a small wrapper that resolves the native printer
  // once and calls it. This avoids a property lookup + host-bridge lookup
  // at every `console.log` call site.
  builtConsole.log = function (...args: Value[]): void {
    try {
      const printer = getNativePrinter();
      // If the resolved printer is the raw host function that expects a
      // single string argument, join with spaces. If it's `native_print`
      // wrapper imported above, it can accept varargs; call accordingly.
      if (printer === host_print) {
        let out = "";
        let sep = "";
        let i = 0;
        while (i < (args as any).length) {
          const a = (args as any)[i];
          let s: string;
          if (a === null) s = "null";
          else if (typeof a === "undefined") s = "undefined";
          else if (typeof a === "string") s = a as any;
          else if (typeof a === "number") s = String(a as any);
          else if (typeof a === "boolean") s = a ? "true" : "false";
          else s = "[object Object]";
          out = out + sep + s;
          sep = " ";
          i = i + 1;
        }
        try {
          printer(out);
        } catch (_) {}
      } else {
        // Printer likely accepts varargs
        try {
          printer(...args);
        } catch (_) {
          // If varargs call fails, fall back to string join
          let out = "";
          let sep = "";
          let i = 0;
          while (i < (args as any).length) {
            const a = (args as any)[i];
            let s: string;
            if (a === null) s = "null";
            else if (typeof a === "undefined") s = "undefined";
            else if (typeof a === "string") s = a as any;
            else if (typeof a === "number") s = String(a as any);
            else if (typeof a === "boolean") s = a ? "true" : "false";
            else s = "[object Object]";
            out = out + sep + s;
            sep = " ";
            i = i + 1;
          }
          try {
            host_print(out);
          } catch (_) {}
        }
      }
    } catch (_) {}
  };
  builtConsole.info = globalConsole.info;
  builtConsole.warn = globalConsole.warn;
  builtConsole.error = globalConsole.error;
  builtConsole.debug = globalConsole.debug;

  if (typeof globalThis !== "undefined") {
    const g: any = globalThis as any;
    try {
      // If host did not provide a console, publish our built console.
      if (!g.console) {
        try {
          g.console = builtConsole;
        } catch (_) {}
      } else {
        // If host provided one, ensure we use a stable set of methods.
        // Copy host methods into a fixed-shape wrapper to avoid polymorphic
        // shapes at call sites that expect `console` to have certain keys.
        const hostConsole = g.console;
        const useConsole: any = {};
        useConsole.log = hostConsole.log
          ? function (...args: any[]) {
              try {
                return hostConsole.log.apply(hostConsole, args);
              } catch (_) {}
            }
          : builtConsole.log;
        useConsole.info = hostConsole.info
          ? function (...args: any[]) {
              try {
                return hostConsole.info.apply(hostConsole, args);
              } catch (_) {}
            }
          : builtConsole.info;
        useConsole.warn = hostConsole.warn
          ? function (...args: any[]) {
              try {
                return hostConsole.warn.apply(hostConsole, args);
              } catch (_) {}
            }
          : builtConsole.warn;
        useConsole.error = hostConsole.error
          ? function (...args: any[]) {
              try {
                return hostConsole.error.apply(hostConsole, args);
              } catch (_) {}
            }
          : builtConsole.error;
        useConsole.debug = hostConsole.debug
          ? function (...args: any[]) {
              try {
                return hostConsole.debug.apply(hostConsole, args);
              } catch (_) {}
            }
          : builtConsole.debug;
        try {
          g.console = useConsole;
        } catch (_) {}
      }
    } catch (_) {
      try {
        g.console = builtConsole;
      } catch (_) {}
    }
    try {
      console = g.console ? g.console : builtConsole;
    } catch (_) {
      console = builtConsole;
    }
  } else {
    console = builtConsole;
  }
} catch (_) {
  try {
    console = globalConsole;
  } catch (_) {}
// Debug: print the assigned console
try {
  host.debugValue("global console assigned", console);
} catch (_) {}

// Basic Object implementation
const ObjectImplInstance: ObjectConstructor = {
  new(value?: Value): BlitzObject {
    if (value === null || value === undefined) {
      return new ObjectImpl();
    }
    if (typeof value === "object") {
      return value as BlitzObject;
    }
    // Would create wrapper objects for primitives
    throw new Error("Wrapper objects not implemented");
  },

  // Removed anonymous call-signature function (tech debt). Implement callable-object behavior
  // properly in the runtime if/when needed.

  keys(o: BlitzObject): string[] {
    return o.getOwnPropertyKeys();
  },

  values(o: BlitzObject): Value[] {
    const keys = o.getOwnPropertyKeys();
    const result: Value[] = [];
    let i = 0;
    while (i < keys.length) {
      result.push(o.get(keys[i]));
      i = i + 1;
    }
    return result;
  },

  entries(o: BlitzObject): [string, Value][] {
    const keys = o.getOwnPropertyKeys();
    const result: [string, Value][] = [];
    let i = 0;
    while (i < keys.length) {
      const key = keys[i];
      result.push([key, o.get(key)]);
      i = i + 1;
    }
    return result;
  },

  assign(target: BlitzObject, ...sources: BlitzObject[]): BlitzObject {
    let si = 0;
    while (si < (sources as any).length) {
      const source = (sources as any)[si];
      const keys = source.getOwnPropertyKeys();
      let ki = 0;
      while (ki < keys.length) {
        const key = keys[ki];
        const value = source.get(key);
        target.set(key, value);
        ki = ki + 1;
      }
      si = si + 1;
    }
    return target;
  },

  create(o: BlitzObject | null): BlitzObject {
    return new ObjectImpl(o);
  },

  defineProperty(o: BlitzObject, p: string, attributes: Value): BlitzObject {
    const descriptor = {
      value: attributes,
      writable: true,
      enumerable: true,
      configurable: true,
    };
    o.defineProperty(p, descriptor);
    return o;
  },

  defineProperties(o: BlitzObject, properties: Value): BlitzObject {
    // Basic implementation
    return o;
  },

  getOwnPropertyDescriptor(o: BlitzObject, p: string): Value {
    return o.getOwnProperty(p) || undefined;
  },

  getOwnPropertyDescriptors(o: BlitzObject): Value {
    const result: Record<string, Value> = {};
    const keys = o.getOwnPropertyKeys();
    let i = 0;
    while (i < keys.length) {
      const key = keys[i];
      const desc = o.getOwnProperty(key);
      if (desc) {
        result[key] = desc;
      }
      i = i + 1;
    }
    return result;
  },

  getOwnPropertyNames(o: BlitzObject): string[] {
    return o.getOwnPropertyKeys();
  },

  getPrototypeOf(o: BlitzObject): BlitzObject | null {
    return o.getPrototypeOf();
  },

  setPrototypeOf(o: BlitzObject, proto: BlitzObject | null): BlitzObject {
    o.setPrototypeOf(proto);
    return o;
  },

  is(x: Value, y: Value): boolean {
    if (x === y) {
      if (x !== 0) return true;
      return 1 / (x as number) === 1 / (y as number);
    }
    return false;
  },

  freeze(o: BlitzObject): BlitzObject {
    o.preventExtensions();
    return o;
  },

  seal(o: BlitzObject): BlitzObject {
    o.preventExtensions();
    return o;
  },

  preventExtensions(o: BlitzObject): BlitzObject {
    o.preventExtensions();
    return o;
  },

  isFrozen(o: BlitzObject): boolean {
    return !o.isExtensible();
  },

  isSealed(o: BlitzObject): boolean {
    return !o.isExtensible();
  },

  isExtensible(o: BlitzObject): boolean {
    return o.isExtensible();
  },
} as ObjectConstructor;

// Construct `Object` constructor object privately and then assign it to
// `globalThis` in a single stable write to avoid producing many transient
// hidden classes during bootstrap.
try {
  const builtObjectCtor: any = ObjectImplInstance;
  if (typeof globalThis !== "undefined") {
    const g: any = globalThis as any;
    try {
      if (!g.Object) {
        try {
          g.Object = builtObjectCtor;
        } catch (_) {}
      }
    } catch (_) {}
    try {
      Object = g.Object ? g.Object : builtObjectCtor;
    } catch (_) {
      Object = builtObjectCtor;
    }
  } else {
    Object = builtObjectCtor;
  }
} catch (_) {
  try {
    Object = ObjectImplInstance;
  } catch (_) {}
}

// Basic String constructor
const StringImplInstance: StringConstructor = {
  new(value?: Value): string {
    return toString(value);
  },

  fromCharCode(...codes: number[]): string {
    return String.fromCharCode(...codes);
  },

  fromCodePoint(...codePoints: number[]): string {
    return String.fromCodePoint(...codePoints);
  },

  raw(template: Value, ...substitutions: Value[]): string {
    // Basic implementation
    return String.raw(template as TemplateStringsArray, ...substitutions);
  },
} as StringConstructor;

// Construct `String` constructor and publish it with a single stable write.
try {
  const builtStringCtor: any = StringImplInstance;
  if (typeof globalThis !== "undefined") {
    const g: any = globalThis as any;
    try {
      if (!g.String) {
        try {
          g.String = builtStringCtor;
        } catch (_) {}
      }
    } catch (_) {}
    try {
      String = g.String ? g.String : builtStringCtor;
    } catch (_) {
      String = builtStringCtor;
    }
  } else {
    String = builtStringCtor;
  }
} catch (_) {
  try {
    String = StringImplInstance;
  } catch (_) {}
}

// Basic Array implementation
const ArrayImplInstance: ArrayConstructor = {
  new(...items: Value[]): Value[] {
    if (items.length === 1 && typeof items[0] === "number") {
      const length = items[0];
      const result: Value[] = new Array(length);
      return result;
    } else {
      return items.slice();
    }
  },

  isArray(arg: Value): boolean {
    // Basic implementation for tsblitz VM
    return Array.isArray(arg);
  },

  from<T>(arrayLike: Value): T[] {
    // Basic implementation
    return [];
  },

  of<T>(...items: T[]): T[] {
    return items.slice();
  },
} as ArrayConstructor;

// Construct `Array` constructor and publish it with a single stable write.
try {
  const builtArrayCtor: any = ArrayImplInstance;
  if (typeof globalThis !== "undefined") {
    const g: any = globalThis as any;
    try {
      if (!g.Array) {
        try {
          g.Array = builtArrayCtor;
        } catch (_) {}
      }
    } catch (_) {}
    try {
      Array = g.Array ? g.Array : builtArrayCtor;
    } catch (_) {
      Array = builtArrayCtor;
    }
  } else {
    Array = builtArrayCtor;
  }
} catch (_) {
  try {
    Array = ArrayImplInstance;
  } catch (_) {}
}

// Ensure an Array.prototype object exists so std library can install methods
// (the runtime's BlitzObject exposes `set`/`defineProperty` and prototype
// behavior; we use `ObjectImpl` to create a fresh prototype object and attach
// it to the constructor-like `Array` implementation).
{
  // Create Array.prototype using the host-provided native object helper
  // when available. `native_new_object` returns a raw VM-backed object and
  // is safer here than constructing a soft-layer class instance because the
  // VM's internal object model expects a native object for prototype links.
  let protoObj: any;
  // Attempt to create a native-backed prototype object via host helper.
  // If the host helper is not available or returns a non-object, fall back
  // to a soft-layer object so bootstrap continues.
  try {
    host.debugValue("native_new_object before use", native_new_object);
  } catch (_) {}
  if (typeof native_new_object === "function") {
    // Call in try/catch to handle host helper errors.
    try {
      protoObj = native_new_object();
      // If the host helper returned null/undefined, fall back
      if (protoObj === null || typeof protoObj === "undefined") {
        protoObj = new ObjectImpl();
      }
    } catch (_e) {
      protoObj = new ObjectImpl();
    }
  } else {
    protoObj = new ObjectImpl();
  }
  // Set constructor on the prototype and attach the prototype to whichever
  // Array constructor we are using (runtime-provided or our soft one). Be
  // defensive: do not overwrite an existing prototype object if present.
    if (protoObj !== null && typeof protoObj !== "undefined") {
    try {
      (protoObj as any)["constructor"] = (Array as any) || ArrayImplInstance;
    } catch (e) {
      // Ignore failures here; host-provided objects may be immutable during
      // certain bootstrap scenarios.
    }

    // Attach to the active Array constructor's prototype if it's missing.
    try {
      // Attach the prototype to the constructor we published above. Prefer
      // the constructor we assigned to `globalThis` to ensure a stable shape.
      const hostCtor = typeof globalThis !== "undefined" ? (globalThis as any).Array : undefined;
      const ctorToUse = hostCtor ? hostCtor : ArrayImplInstance;
      if (ctorToUse != null && (typeof ctorToUse === "object" || typeof ctorToUse === "function")) {
        try {
          if (ctorToUse.prototype == null) {
            ctorToUse.prototype = protoObj;
          }
        } catch (_) {
          // Ignore failures — host objects may be immutable during bootstrap.
        }
      }
    } catch (_) {}

    // Debug: verify that prototype was set on the active constructor
    try {
      let readBack: any = (typeof globalThis !== "undefined" && (globalThis as any).Array)
        ? (globalThis as any).Array.prototype
        : (ArrayImplInstance as any).prototype;
      try {
        host.debugValue("Array.prototype post-assign", readBack);
      } catch (_) {}
    } catch (_) {}
  }
}

// Run any postponed std installers that were queued while std modules
// initialized before the prelude completed. These installers expect the
// runtime-provided constructors (Array, String, etc.) to exist and will
// attach prototype methods. Run them now, best-effort.
  try {
    if (typeof globalThis !== "undefined") {
      const g: any = globalThis as any;
    const arr = g.__tsblitz_post_bootstrap;
    // Use a defensive array detection because `Array` may be null/host-provided
    // and calling `Array.isArray` could itself throw. Treat a value as an array
    // if it looks like an array (has numeric `length` and `push` function).
    const looksLikeArray = arr != null && typeof arr === "object" && typeof (arr as any).length === "number" && typeof (arr as any).push === "function";
    if (looksLikeArray) {
        const remaining: any[] = [];
        let i = 0;
        while (i < arr.length) {
          try {
            const entry = arr[i];
            // Support both legacy function installers and the new descriptor
            // objects pushed by std modules.
            const installer = typeof entry === "function" ? entry : entry && entry.fn;
            const targetName = entry && entry.target ? entry.target : "Array";
            if (typeof installer === "function") {
              try {
                const hostCtor = g && g[targetName];
                const usableHostCtor = hostCtor != null && (typeof hostCtor === "object" || typeof hostCtor === "function");
                // If host ctor is usable, pass it; otherwise pass undefined and
                // allow the installer or prelude fallback to handle it.
                const ok = installer(usableHostCtor ? hostCtor : undefined);
                // If installer explicitly returned false, keep it for later
                if (ok === false) remaining.push(entry);
              } catch (_) {
                // If installer threw, keep it for a single retry later.
                remaining.push(entry);
              }
            }
          } catch (_) {
            // Swallow and continue
          }
          i = i + 1;
        }
        try {
          g.__tsblitz_post_bootstrap = remaining;
        } catch (_) {}
      }
    }
  } catch (_) {}
}
