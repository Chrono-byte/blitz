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
// and `debugValue`. We access them directly as globals after bundling.

// Load the standard library so Array.prototype, Object helpers and other
// std APIs are available during bootstrap. The bundler will inline the
// referenced modules into the prelude program.
// Import std modules explicitly (avoid `export *` patterns the bundler
// currently doesn't support).
import "./std/console.js";
import "./std/object.js";
import "./std/array.js";
import "./std/string.js";

// Access host-provided primitives via the single accessor the kernel exposes.
// We avoid `declare`/TS ambient declarations because the project's parser
// doesn't accept `TSDeclareFunction`. Instead capture the runtime global
// accessor (if present) into a safe local function reference.
// Reuse host helper module for safe access to VM-provided primitives.
// Local aliases to host helpers (these are defined by the inlined host.ts).
// Use `typeof` checks to be defensive during bootstrap.
const __host_get_fn: any = typeof __host_get_fn !== "undefined" ? __host_get_fn : undefined;
function host_print(msg: string): void {
  try {
    if (typeof host_print === "function") {
      (host_print as any)(msg);
    }
  } catch (_) {}
}
const native_new_object: any = typeof native_new_object !== "undefined" ? native_new_object : undefined;

// Declare global variables
// Capture runtime globals (console, Object, Array, String) when available so
// the bundler-generated TypeScript files can reference them without
// relying on unsupported `declare` statements.
const console: any =
  typeof globalThis !== "undefined" && (globalThis as any).console
    ? (globalThis as any).console
    : { log: function (..._args: any[]) {} };
const Object: any =
  typeof globalThis !== "undefined" && (globalThis as any).Object ? (globalThis as any).Object : {};
const Array: any =
  typeof globalThis !== "undefined" && (globalThis as any).Array ? (globalThis as any).Array : {};
const String: any =
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
    host_print(out);
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
    host_print(out);
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
    host_print(out);
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
    host_print(out);
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
    host_print(out);
  },
};

// Assign to global console
(console as unknown) = globalConsole;
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

// Assign to global Object
(Object as unknown) = ObjectImplInstance;

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

// Assign to global String
(String as unknown) = StringImplInstance;

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

// Assign to global Array
(Array as unknown) = ArrayImplInstance;

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
  // Set constructor on the prototype using plain property assignment so we
  // don't depend on a `.set` helper being present during bootstrap.
  // Only set properties if we actually have an object. Some bootstrap
  // environments may return `null`/`undefined` or reject assignments; be
  // defensive so the prelude still completes.
  if (protoObj !== null && typeof protoObj !== "undefined") {
    try {
      (protoObj as any)["constructor"] = ArrayImplInstance as any;
    } catch (e) {
      // Ignore failures here; the runtime may not accept assignment to the
      // host-provided object in some bootstrap scenarios.
    }
    // Attach prototype to the ArrayImplInstance so `Array.prototype` lookups work
    try {
      (ArrayImplInstance as any).prototype = protoObj;
    } catch (e) {
      // noop
    }
    // Debug: verify that prototype was set on the ArrayImplInstance
    try {
      let readBack: any = (ArrayImplInstance as any).prototype;
      try {
        host.debugValue("Array.prototype post-assign", readBack);
      } catch (_) {}
    } catch (_) {}
  }
}
